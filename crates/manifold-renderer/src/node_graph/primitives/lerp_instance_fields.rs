//! `node.blend_copies` — elementwise linear interpolation
//! between two `Array<InstanceTransform>`s.
//!
//! ```text
//! out[idx].pos_scale = (1 - t) * a.pos_scale + t * b.pos_scale
//! out[idx].rot_pad   = (1 - t) * a.rot_pad   + t * b.rot_pad
//! ```
//!
//! Pair with `node.cylinder_wrap_field` / `node.torus_wrap_field` (or
//! any two topology-derived `Array<InstanceTransform>`s) to morph
//! continuously between them — what `node.switch_array` can't do (it
//! selects one of N discretely, at the lowest integer index of the
//! selector). For the canonical DigitalPlants morph (cyl ↔ tor), `t
//! = morph` is wired from the outer card.

use std::borrow::Cow;
use manifold_gpu::GpuBinding;

use crate::generators::mesh_common::InstanceTransform;
use crate::node_graph::effect_node::EffectNodeContext;
use crate::node_graph::parameters::{ParamDef, ParamType, ParamValue};
use crate::node_graph::primitive::Primitive;
use super::standalone_pipeline::standalone_pipeline;

/// Generated-codegen uniform layout: the `t` param (f32) then the codegen-
/// injected `dispatch_count` (= element count, the guard), padded to 16 bytes.
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Uniforms {
    t: f32,
    dispatch_count: u32,
    _pad0: u32,
    _pad1: u32,
}

crate::primitive! {
    name: LerpInstanceFields,
    type_id: "node.blend_copies",
    purpose: "Elementwise linear interpolation between two Array<InstanceTransform>s. out[idx] = (1 - t) * a[idx] + t * b[idx] applied to both pos_scale and rot_pad. The continuous counterpart to node.switch_array — pick this when the morph parameter is a real 0..1 slider and intermediate values must visually morph (the DigitalPlants cyl ↔ tor case). At t=0 the output equals a; at t=1 it equals b; at t=0.5 the elementwise midpoint. `t` is port-shadow-param so the morph factor can be modulated.",
    inputs: {
        a: Array(InstanceTransform) required,
        b: Array(InstanceTransform) required,
        t: ScalarF32 optional,
    },
    outputs: {
        out: Array(InstanceTransform),
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
    composition_notes: "Output capacity follows `a` (and run() truncates dispatch to min(a, b, out) so the shader can't read past either input). `t` is not clamped — values outside [0, 1] produce honest extrapolation, useful for over- or under-shoot effects. Both pos_scale.w (instance scale) and rot_pad get lerped — when both upstream sources write the same scale and zero rotation those fields stay invariant under the lerp, leaving the perceptible effect on pos.xyz alone.",
    examples: [],
    picker: { label: "Blend Copies", category: Atom },
    summary: "Blends two arrangements of copies together by an amount, so you can morph a field of copies from one layout to another.",
    category: Particles2D,
    role: Filter,
    aliases: ["blend copies", "lerp instance fields", "morph", "lerp", "interpolate"],
    fusion_kind: Pointwise,
    wgsl_body: include_str!("shaders/lerp_instance_fields_body.wgsl"),
}

impl Primitive for LerpInstanceFields {
    fn array_output_capacity(
        &self,
        port_name: &str,
        _params: &crate::node_graph::effect_node::ParamValues,
        input_capacities: &[(&str, u32)],
    ) -> Option<u32> {
        if port_name != "out" {
            return None;
        }
        input_capacities
            .iter()
            .find(|(p, _)| *p == "a")
            .map(|(_, n)| *n)
    }

    fn run(&mut self, ctx: &mut EffectNodeContext<'_, '_>) {
        let t = ctx.scalar_or_param("t", 0.5);

        let Some(a_buf) = ctx.inputs.array("a") else {
            return;
        };
        let Some(b_buf) = ctx.inputs.array("b") else {
            return;
        };
        let Some(out_buf) = ctx.outputs.array("out") else {
            return;
        };

        let inst_size = std::mem::size_of::<InstanceTransform>() as u64;
        let a_cap = (a_buf.size / inst_size) as u32;
        let b_cap = (b_buf.size / inst_size) as u32;
        let out_cap = (out_buf.size / inst_size) as u32;
        let count = a_cap.min(b_cap).min(out_cap);
        if count == 0 {
            return;
        }

        let gpu = ctx.gpu_encoder();
        let pipeline = standalone_pipeline::<Self>(&mut self.pipeline, gpu.device);

        let uniforms = Uniforms {
            t,
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
                    buffer: out_buf,
                    offset: 0,
                },
            ],
            [count.div_ceil(256), 1, 1],
            "node.blend_copies",
        );
    }
}

