//! `node.field_smooth_blend`: the water field build's weighted, leashed blend
//! after each smoothing step. A per-node atom that fuses with the z smoothing
//! pass feeding it.
use crate::field_build::{FIELD_REDISTANCE_CELLS, FIELD_SMOOTHNESS_MAX};
use crate::float_param;
use manifold_gpu::GpuBinding;
use manifold_node_engine::exec::effect_node::{EffectNodeContext, ParamValues};
use manifold_node_engine::parameters::{ParamDef, ParamType, ParamValue};
use manifold_node_engine::primitive::Primitive;
use manifold_node_engine::primitives::standalone_pipeline::standalone_pipeline;
use std::borrow::Cow;

/// Codegen uniform layout: params in PARAMS order, then `dispatch_count`.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    nodes_x: f32,
    nodes_y: f32,
    nodes_z: f32,
    size_x: f32,
    size_y: f32,
    size_z: f32,
    smoothness: f32,
    band_cells: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
    _pad2: u32,
}

manifold_node_engine::primitive! {
    name: FieldSmoothBlend,
    type_id: "node.field_smooth_blend",
    purpose: "Blend a smoothed scalar lattice back toward the field it was smoothed from, per node, and hold the result within a leash of a reference field. Node (i, j, k) of the nodes_x/y/z lattice is at i + nx·(j + ny·k); size_x/y/z is the lattice box in metres, so one cell is the smallest of size / (nodes - 1) over the axes. Where |reference| is at least band_cells cells the prior passes through unchanged. Elsewhere the output is prior + weight · (smoothed - prior), clamped to reference ± smoothness × FIELD_LEASH_CELLS (half a cell). No lattice (fewer than 2 nodes on an axis) copies the smoothed value.",
    inputs: {
        smoothed: Array(f32) required,
        prior: Array(f32) required,
        weights: Array(f32) required,
        reference: Array(f32) required,
        nodes_x: ScalarF32 optional, nodes_y: ScalarF32 optional, nodes_z: ScalarF32 optional,
        size_x: ScalarF32 optional, size_y: ScalarF32 optional, size_z: ScalarF32 optional,
        smoothness: ScalarF32 optional, band_cells: ScalarF32 optional,
    },
    outputs: { field: Array(f32), },
    params: [
        float_param!("nodes_x", "Nodes X", 2.0, 2.0, 4096.0),
        float_param!("nodes_y", "Nodes Y", 2.0, 2.0, 4096.0),
        float_param!("nodes_z", "Nodes Z", 2.0, 2.0, 4096.0),
        float_param!("size_x", "Size X", 4.0, 0.001, 1000.0),
        float_param!("size_y", "Size Y", 4.0, 0.001, 1000.0),
        float_param!("size_z", "Size Z", 4.0, 0.001, 1000.0),
        float_param!("smoothness", "Smoothness", 1.0, 0.0, FIELD_SMOOTHNESS_MAX),
        float_param!("band_cells", "Band Cells", FIELD_REDISTANCE_CELLS, 0.0, 16.0),
    ],
    depth_rule: Terminal,
    composition_notes: "Place after the z node.smooth_lattice pass of each smoothing step: smoothed is that pass, prior is the field the step started from, reference is the re-distanced field, weights come from node.surface_weights. Wire nodes_x/y/z from the volume's lattice and size_x/y/z from its box in metres. Smoothness 1 is the tuned surface (the field build's half-cell leash), 0 returns the re-distanced reference, 2 doubles the leash; band defaults to the field build's constant.",
    examples: [],
    picker: { label: "Field Smooth Blend", category: Atom },
    summary: "Eases a smoothed liquid surface back toward the original where it is thin, and never lets it drift far from the true surface.",
    category: Particles3D,
    role: Filter,
    aliases: ["smoothing blend", "leashed smoothing", "weighted smoothing"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/field_smooth_blend_body.wgsl"),
    input_access: [Coincident, Coincident, Coincident, Coincident],
}

impl Primitive for FieldSmoothBlend {
    fn array_output_capacity(
        &self,
        port: &str,
        _params: &ParamValues,
        inputs: &[(&str, u32)],
    ) -> Option<u32> {
        (port == "field")
            .then(|| {
                inputs
                    .iter()
                    .find(|(name, _)| *name == "smoothed")
                    .map(|&(_, n)| n)
            })
            .flatten()
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let nodes =
            ["nodes_x", "nodes_y", "nodes_z"].map(|name| ctx.scalar_or_param(name, 2.0).round());
        let size = ["size_x", "size_y", "size_z"].map(|name| ctx.scalar_or_param(name, 4.0));
        let smoothness = ctx.scalar_or_param("smoothness", 1.0);
        let band_cells = ctx.scalar_or_param("band_cells", FIELD_REDISTANCE_CELLS);
        let (Some(smoothed), Some(prior), Some(weights), Some(reference), Some(field)) = (
            ctx.inputs.array("smoothed"),
            ctx.inputs.array("prior"),
            ctx.inputs.array("weights"),
            ctx.inputs.array("reference"),
            ctx.outputs.array("field"),
        ) else {
            let missing = [
                ("smoothed", ctx.inputs.array("smoothed").is_none()),
                ("prior", ctx.inputs.array("prior").is_none()),
                ("weights", ctx.inputs.array("weights").is_none()),
                ("reference", ctx.inputs.array("reference").is_none()),
                ("field", ctx.outputs.array("field").is_none()),
            ]
            .into_iter()
            .filter_map(|(name, absent)| absent.then_some(name))
            .collect::<Vec<_>>()
            .join(", ");
            ctx.error(format!("Field Smooth Blend: missing {missing}"));
            return;
        };
        if nodes.iter().any(|&n| n < 2.0) {
            ctx.error(format!(
                "Field Smooth Blend: the {}×{}×{} lattice needs at least 2 nodes on every axis",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let needed: u64 = nodes.iter().map(|&n| n as u64).product();
        let short = [
            ("smoothed", smoothed.size / 4),
            ("prior", prior.size / 4),
            ("weights", weights.size / 4),
            ("reference", reference.size / 4),
            ("field", field.size / 4),
        ]
        .into_iter()
        .find(|&(_, have)| have < needed);
        if let Some((name, have)) = short {
            ctx.error(format!(
                "Field Smooth Blend: {name} holds {have} values but the {}×{}×{} lattice needs {needed}",
                nodes[0], nodes[1], nodes[2]
            ));
            return;
        }
        let count = needed as u32;
        let uniforms = Uniforms {
            nodes_x: nodes[0],
            nodes_y: nodes[1],
            nodes_z: nodes[2],
            size_x: size[0],
            size_y: size[1],
            size_z: size[2],
            smoothness,
            band_cells,
            dispatch_count: count,
            _pad0: 0,
            _pad1: 0,
            _pad2: 0,
        };
        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);
        gpu.native_enc.dispatch_compute(
            pipeline,
            &[
                GpuBinding::Bytes {
                    binding: 0,
                    data: bytemuck::bytes_of(&uniforms),
                },
                GpuBinding::Buffer {
                    binding: 1,
                    buffer: smoothed,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 2,
                    buffer: prior,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 3,
                    buffer: weights,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 4,
                    buffer: reference,
                    offset: 0,
                },
                GpuBinding::Buffer {
                    binding: 5,
                    buffer: field,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.field_smooth_blend",
        );
    }
}

#[cfg(any(test, feature = "testkit", feature = "gpu-proofs"))]
pub(crate) mod extent;

#[cfg(all(test, feature = "gpu-proofs"))]
mod gpu_tests;
