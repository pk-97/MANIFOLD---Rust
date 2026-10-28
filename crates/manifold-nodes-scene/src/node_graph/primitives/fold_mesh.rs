//! `node.fold_mesh` — single-axis mirror fold of an `Array<MeshVertex>`.
//!
//! Per vertex: `pos = mix(pos, reflect(pos), amount)`, where `reflect` flips the
//! coordinate along the chosen `axis` (mirror across the plane through the origin
//! whose normal is the axis). The normal is reflected by the same amount so the
//! mirrored half lights correctly. `w` is the optional per-vertex `weights` input
//! (degrading to 1.0 past a short/unwired buffer).

use std::borrow::Cow;

use manifold_gpu::GpuBinding;

use manifold_node_engine::exec::effect_node::EffectNodeContext;
use manifold_node_engine::mesh::MeshVertex;
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;

const FOLD_AXES: &[&str] = &["X", "Y", "Z"];

/// Generated-codegen uniform layout: scalar params in PARAMS order (`axis`
/// Enum→u32, `amount` f32), then the derived `weights_len` (u32), then the
/// codegen-injected `dispatch_count`, padded to a 16-byte multiple. 4 words =
/// 16 bytes. Matches `standalone_for_spec::<FoldMesh>()`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct FoldUniforms {
    axis: u32,
    amount: f32,
    source_offset_x: f32,
    source_offset_y: f32,
    source_offset_z: f32,
    weights_len: u32,
    dispatch_count: u32,
    _pad0: u32,
}

manifold_node_engine::primitive! {
    name: FoldMesh,
    type_id: "node.fold_mesh",
    purpose: "Single-axis mirror fold of an Array<MeshVertex>. pos = mix(pos, reflect(pos), amount), where reflect flips the coordinate along the chosen axis (mirror across the plane through the origin whose normal is the axis). The normal is reflected by the same amount so the mirrored half lights correctly. `w` is the optional per-vertex `weights` input (a short or unwired weights buffer degrades to 1.0, never silent 0). source_offset_x/y/z shift the frame: the vertex is deformed at position + source_offset and shifted back, so the parts of a group, each offset by its place in the group, deform as one piece about the frame's origin (all zero = the mesh's own origin).",
    inputs: {
        in: Array(MeshVertex) required,
        weights: Array(f32) optional,
        amount: ScalarF32 optional,
        source_offset_x: ScalarF32 optional,
        source_offset_y: ScalarF32 optional,
        source_offset_z: ScalarF32 optional,
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
            range: Some((0.0, (FOLD_AXES.len() - 1) as f32)),
            enum_values: FOLD_AXES,
        },
        ParamDef {
            name: Cow::Borrowed("amount"),
            label: "Amount",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: Some((0.0, 1.0)),
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("source_offset_x"),
            label: "Source Offset X",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("source_offset_y"),
            label: "Source Offset Y",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
        ParamDef {
            name: Cow::Borrowed("source_offset_z"),
            label: "Source Offset Z",
            ty: ParamType::Float,
            default: ParamValue::Float(0.0),
            range: None,
            enum_values: &[],
        },
    ],
    depth_rule: Terminal,
    composition_notes: "The 'kaleido fold' deformer: chain multiple Folds on different axes to build N-section mirror symmetry. Wire node.mesh_ramp's `weights` output to grow the fold from one side of the mesh. Because normals are reflected with positions, the folded half lights correctly without a downstream facet_normals reset.",
    examples: [],
    picker: { label: "Fold", category: Atom },
    summary: "Mirrors a mesh across a plane through the origin along one axis, with adjustable blend amount — the building block for kaleidoscope geometry.",
    category: Geometry3D,
    role: Filter,
    aliases: ["fold", "fold mesh", "mirror fold", "kaleidoscope"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/fold_mesh_body.wgsl"),
    // `in` and `weights` are both COINCIDENT (default) — keeps the atom fully
    // pointwise/fusable so it can chain with other mesh deformers. `weights_len`
    // is a frame-derived uniform the body uses to bounds-check the coincident
    // weight read (degrade to 1.0 past the buffer).
    derived_uniforms: ["weights_len:u32"],
}

// Per-frame recompute for a FUSED region's derived block: `weights_len` is
// the live element count of the wired `weights` buffer (0 when unwired — the
// body's `idx < weights_len` gate degrades every weight to 1.0, exactly what
// `run()` does). The marker carries the member→fused-port mapping for the
// `weights` port (fused kernels rename inputs to `src_<k>`).
inventory::submit! {
    manifold_node_engine::freeze::derived_uniform_registry::DerivedUniformRecompute {
        type_id: "node.fold_mesh",
        array_ports: &["weights"],
        recompute: |ctx| Some(vec![(ctx.array_len)("weights").unwrap_or(0) as f32]),
    }
}

impl Primitive for FoldMesh {
    fn mesh_output_rule(
        &self,
        port: &str,
    ) -> manifold_node_engine::scene::mesh_change::MeshOutputRule<'_> {
        manifold_node_engine::scene::mesh_change::MeshOutputRule::deformer(
            port,
            manifold_node_engine::scene::mesh_change::TOPOLOGY_OF_IN,
        )
    }

    /// Output `out` is sized to match input `in` — folding is a per-vertex
    /// transform, no expansion.
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &manifold_node_engine::exec::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "in")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let axis = match ctx.params.get("axis") {
            Some(ParamValue::Enum(v)) => (*v).min((FOLD_AXES.len() - 1) as u32),
            _ => 1,
        };
        let amount = ctx.scalar_or_param("amount", 0.0);
        let source_offset_x = ctx.scalar_or_param("source_offset_x", 0.0);
        let source_offset_y = ctx.scalar_or_param("source_offset_y", 0.0);
        let source_offset_z = ctx.scalar_or_param("source_offset_z", 0.0);

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
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = FoldUniforms {
            axis,
            amount,
            source_offset_x,
            source_offset_y,
            source_offset_z,
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
            "node.fold_mesh",
        );
    }
}
